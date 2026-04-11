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
  %t5 = add i32 0, 2
  %t6 = srem i32 %t4, %t5
  %t7 = add i32 0, 0
  %t8 = icmp eq i32 %t6, %t7
  br i1 %t8, label %bb5, label %bb6
bb3:
  %t9 = add i32 0, 1
  %t10 = load i32, ptr %merge_2
  %t11 = add i32 %t10, %t9
  store i32 %t11, ptr %merge_2
  br label %bb1
bb4:
  ret i32 0
bb5:
  br label %bb3
bb6:
  br label %bb7
bb7:
  %t12 = load i32, ptr %merge_2
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t12)
  br label %bb3
}

