; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 1
  %t1 = add i32 0, 10
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb4
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = add i32 0, 5
  %t6 = icmp eq i32 %t4, %t5
  br i1 %t6, label %bb5, label %bb6
bb3:
  %t7 = add i32 0, 1
  %t8 = load i32, ptr %merge_2
  %t9 = add i32 %t8, %t7
  store i32 %t9, ptr %merge_2
  br label %bb1
bb4:
  ret i32 0
bb5:
  br label %bb4
bb6:
  br label %bb7
bb7:
  %t10 = load i32, ptr %merge_2
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t10)
  br label %bb3
}

