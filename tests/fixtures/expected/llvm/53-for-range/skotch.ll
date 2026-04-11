; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 1
  %t1 = add i32 0, 5
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_2
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 1
  %t7 = load i32, ptr %merge_2
  %t8 = add i32 %t7, %t6
  store i32 %t8, ptr %merge_2
  br label %bb1
bb3:
  ret i32 0
}

