; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_sign(i32 %arg0) {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 0
  %t2 = icmp sgt i32 %arg0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  %t3 = add i32 0, 1
  store i32 %t3, ptr %merge_2
  br label %bb6
bb3:
  %t4 = add i32 0, 0
  %t5 = icmp slt i32 %arg0, %t4
  br i1 %t5, label %bb4, label %bb5
bb4:
  %t6 = add i32 0, -1
  store i32 %t6, ptr %merge_2
  br label %bb6
bb5:
  %t7 = add i32 0, 0
  store i32 %t7, ptr %merge_2
  br label %bb6
bb6:
  %t8 = load i32, ptr %merge_2
  ret i32 %t8
}

define i32 @main() {
entry:
  %t0 = add i32 0, 42
  %t1 = call i32 @InputKt_sign(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, -7
  %t4 = call i32 @InputKt_sign(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 0
  %t7 = call i32 @InputKt_sign(i32 %t6)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  ret i32 0
}

